//! Z.ai's coding API: Chat Completions on its PaaS endpoint, serving GLM coding models.

use std::time::Duration;

use super::*;
use anyhow::{Context, Result, bail};

/// Where Z.ai's models answer.
pub const BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";

/// What `/provider zai` writes, said before the key is asked.
pub const ABOUT: &str = "Queries Z.ai's model list at https://api.z.ai/api/coding/paas/v4/models and writes a `zai` provider on the Chat Completions API with every model it lists, or repairs the one the config has. The key is Z.ai's API key; a wrong one shows as HTTP 401 on the first request.";

/// The configured provider that is Z.ai: the one on Z.ai's API.
pub fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.trim_end_matches('/') == BASE_URL)
}

/// `/provider zai`: writes the provider and the models its list endpoint names with
/// `api_key`. A Z.ai provider the config has already is repaired: it gets the preset's API
/// and URL. A failed list call falls back to GLM-5.3, so a network hiccup doesn't cost the
/// provider.
pub async fn add(api_key: &str) -> Result<Added> {
    add_with(("api_key", api_key), api_key).await
}

/// The same, for a provider whose key is kept in `api_key_env` rather than inline: the
/// list is fetched with the resolved key, but the provider keeps the variable's name.
pub async fn add_env(env: &str, api_key: &str) -> Result<Added> {
    add_with(("api_key_env", env), api_key).await
}

async fn add_with(key_field: (&str, &str), api_key: &str) -> Result<Added> {
    let served = served(api_key).await;
    let (ids, listed) = match &served {
        Ok(ids) => (ids.clone(), true),
        Err(_) => (vec!["glm-5.3".to_string()], false),
    };
    let mut added = add_models(&additions(key_field, ids.clone()))?;
    if listed {
        let config = Config::load()?;
        let keep = crate::config::kept_models(&config, &added.provider, &ids);
        added.dropped = crate::config::remove_models(&added.path, &added.provider, &keep)?;
    } else if let Err(error) = served {
        added.notes.push(format!(
            "could not list Z.ai's models ({error:#}); kept the models already in the config and GLM-5.3"
        ));
    }
    Ok(added)
}

/// The model ids the coding API lists for `api_key`: `GET /models`.
async fn served(api_key: &str) -> Result<Vec<String>> {
    let http = reqwest::Client::builder()
        .user_agent(crate::llm::USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()?;
    let url = format!("{BASE_URL}/models");
    let response = http
        .get(&url)
        .bearer_auth(api_key)
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
    let ids: Vec<String> = body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| model["id"].as_str())
        .map(String::from)
        .collect();
    if ids.is_empty() {
        bail!("{url} listed no models");
    }
    Ok(ids)
}

/// The provider and models the preset writes: the Chat Completions API on Z.ai's URL and
/// every model the list endpoint named.
fn additions(key_field: (&str, &str), ids: Vec<String>) -> Additions {
    let (field, value) = key_field;
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert(field.into(), value.into());
    Additions {
        name: "zai",
        ours,
        provider,
        models: ids.into_iter().map(|id| glm(&id)).collect(),
        comment: "Z.ai, added with /provider zai.",
    }
}

fn glm(id: &str) -> NewModel {
    let context = if id.starts_with("glm-5.3") {
        1_000_000
    } else {
        128_000
    };
    NewModel {
        id: id.into(),
        context: Some(context),
        output: Some(128_000),
        thinking: true,
        tools: true,
        default_effort: Some("high".into()),
        efforts: vec!["high".into()],
        ..NewModel::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one model the preset wrote before it read the list endpoint.
    fn sample() -> Vec<String> {
        vec!["glm-5.3".to_string()]
    }

    /// The preset, rendered into an empty config the way `add_models` writes it.
    fn written() -> String {
        let (out, _, _, added) = models::with_models("", &additions(("api_key", "sk-zai"), sample())).unwrap();
        assert_eq!(added, ["zai/glm-5.3"]);
        out
    }

    #[test]
    fn every_model_the_list_endpoint_names_is_written() {
        let ids = vec!["glm-5.3".to_string(), "glm-5.3-flash".to_string()];
        let (out, _, _, added) = models::with_models("", &additions(("api_key", "sk-zai"), ids)).unwrap();
        assert_eq!(added, ["zai/glm-5.3", "zai/glm-5.3-flash"]);
        assert!(
            out.contains("[models.\"zai/glm-5.3-flash\"]"),
            "{out}"
        );
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(
            config.models["zai/glm-5.3-flash"].max_context_size,
            Some(1_000_000)
        );
    }

    #[test]
    fn the_provider_is_openai_on_zais_url() {
        let out = written();
        assert!(out.contains("type = \"openai\""), "{out}");
        assert!(
            out.contains("base_url = \"https://api.z.ai/api/coding/paas/v4\""),
            "{out}"
        );
    }

    #[test]
    fn a_config_without_zai_gets_the_provider() {
        let mine = "default_model = \"openai/gpt\"\n\n[providers.openai]\ntype = \"openai\"\napi_key = \"sk\"\n\n[models.\"openai/gpt\"]\nprovider = \"openai\"\nmodel = \"gpt\"\n";
        let (out, name, added, repaired) = with_additions(mine, &additions(("api_key", "sk-zai"), sample())).unwrap();
        assert_eq!((name.as_str(), added.len()), ("zai", 1));
        assert!(repaired.is_empty(), "nothing was there to repair");
        assert!(out.starts_with(mine), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = config.resolve(Some("zai/glm-5.3")).unwrap();
        assert_eq!(model.protocol, Protocol::OpenAi);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "sk-zai");
        assert_eq!(config.default_model.as_deref(), Some("openai/gpt"));
    }

    #[test]
    fn a_zai_provider_the_config_had_is_repaired_and_keeps_the_rest() {
        let broken = "default_model = \"zai/glm-5.3\"\n\n\
            # mine\n[providers.zai]\ntype = \"anthropic\"\nbase_url = \"https://api.z.ai/api/coding/paas/v4\"\napi_key_env = \"ZAI_KEY\"\ncustom_headers = { x-team = \"a\" }\n\n\
            # my model\n[models.\"zai/glm-5.3\"]\nprovider = \"zai\"\nmodel = \"glm-5.3\"\ndefault_effort = \"high\"\n";
        let (out, name, added, repaired) = with_additions(broken, &additions(("api_key", "sk-new"), sample())).unwrap();
        assert_eq!(name, "zai");
        assert!(added.is_empty(), "the model was already there: {out}");
        assert_eq!(repaired, ["type"], "{out}");
        assert!(out.contains("# mine\n[providers.zai]\n"), "{out}");
        assert!(out.contains("# my model\n"), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let zai = &config.providers["zai"];
        assert_eq!(zai.kind, "openai");
        assert_eq!(
            (zai.api_key.as_deref(), zai.api_key_env.as_deref()),
            (Some("sk-new"), None)
        );
        assert_eq!(zai.custom_headers["x-team"], "a");
        let model = config.resolve(None).unwrap();
        assert_eq!(model.protocol, Protocol::OpenAi);
        assert_eq!(model.base_url, BASE_URL);

        let (again, _, none, nothing) = with_additions(&out, &additions(("api_key", "sk-new"), sample())).unwrap();
        assert!(
            again == out && none.is_empty() && nothing.is_empty(),
            "a repaired config needs nothing more: {again}"
        );
    }

    #[test]
    fn only_the_coding_api_is_ours() {
        for url in [
            BASE_URL,
            "https://api.z.ai/api/coding/paas/v4/",
        ] {
            let provider: Provider = toml::from_str(&format!(
                "type = \"openai\"\nbase_url = \"{url}\"\napi_key = \"k\"\n"
            ))
            .unwrap();
            assert!(ours(&provider), "{url}");
        }
        for url in [
            "https://api.z.ai-mirror.example/v4",
            "https://api.z.ai/api/paas/v4",
        ] {
            let provider: Provider = toml::from_str(&format!(
                "type = \"openai\"\nbase_url = \"{url}\"\napi_key = \"k\"\n"
            ))
            .unwrap();
            assert!(!ours(&provider), "{url}");
        }
    }

    #[test]
    fn a_zai_provider_on_another_api_is_left_alone() {
        let other = "[providers.zai]\ntype = \"openai\"\nbase_url = \"https://api.example.com/v1\"\napi_key = \"other\"\n";
        let (out, name, _, repaired) = with_additions(other, &additions(("api_key", "sk-zai"), sample())).unwrap();
        assert_eq!((name.as_str(), repaired.len()), ("zai-2", 0));
        assert!(out.contains(other), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(config.providers["zai"].api_key.as_deref(), Some("other"));
        assert_eq!(
            config.providers["zai-2"].base_url.as_deref(),
            Some(BASE_URL)
        );
    }

    #[test]
    fn the_model_keeps_glm_capabilities() {
        let out = written();
        let config: Config = toml::from_str(&out).unwrap();
        let model = &config.models["zai/glm-5.3"];
        assert_eq!(model.capabilities, ["thinking", "tool_use"]);
        assert_eq!(model.default_effort.as_deref(), Some("high"));
        assert_eq!(model.support_efforts, ["high"]);
    }
}
