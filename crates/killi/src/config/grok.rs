//! xAI's Grok models on its Responses API (docs.x.ai), with an API key from console.x.ai.
//! `killi login grok` writes the same models on the user's Grok subscription instead.

use super::*;
use anyhow::Result;

/// Where xAI's models answer.
pub const BASE_URL: &str = "https://api.x.ai/v1";

/// What `/provider grok` writes, said before the key is asked.
pub const ABOUT: &str = "Writes an `xai` provider on the Responses API at https://api.x.ai/v1 with the key from console.x.ai, or repairs the one the config has; a wrong one shows as HTTP 400 on the first request. `killi login grok` uses a SuperGrok or X Premium subscription instead.";

/// The configured provider that is xAI with a key: the one on xAI's API, not a plan.
pub fn ours(provider: &Provider) -> bool {
    Plan::of(&provider.kind).is_none()
        && provider
            .base_url
            .as_deref()
            .is_some_and(|url| url.starts_with("https://api.x.ai"))
}

/// `/provider grok`: writes the provider and its models with `api_key`. An xAI provider the
/// config has already is repaired: it gets the preset's API and URL.
pub fn add(api_key: &str) -> Result<Added> {
    add_models(&additions(api_key))
}

fn additions(api_key: &str) -> Additions {
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai_responses".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: "xai".into(),
        ours,
        provider,
        models: models(),
        comment: "xAI's Grok models, added with /provider grok.",
    }
}

/// The Grok models killi writes, from docs.x.ai/developers/models: text and images in,
/// reasoning that can't be switched off, and `xhigh` as the deepest effort.
pub fn models() -> Vec<NewModel> {
    vec![
        grok("grok-4.7", "Grok 4.7", 500_000),
        grok("grok-4.6", "Grok 4.6", 500_000),
        grok("grok-4.3", "Grok 4.3", 1_000_000),
    ]
}

fn grok(id: &str, display: &'static str, context: u64) -> NewModel {
    NewModel {
        id: id.into(),
        context: Some(context),
        images: true,
        thinking: true,
        tools: true,
        more_capabilities: vec!["always_thinking"],
        display: Some(display),
        default_effort: Some("high".into()),
        efforts: ["low", "medium", "high", "xhigh"]
            .into_iter()
            .map(Into::into)
            .collect(),
        ..NewModel::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provider_speaks_responses_on_xais_api() {
        let (out, name, added, _) = with_additions("", &additions("xai-key")).unwrap();
        assert_eq!(name, "xai");
        assert_eq!(added, ["xai/grok-4.7", "xai/grok-4.6", "xai/grok-4.3"]);
        let config: Config = toml::from_str(&out).unwrap();
        let model = config.resolve(Some("xai/grok-4.7")).unwrap();
        assert_eq!(model.protocol, Protocol::Responses);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "xai-key");
        assert_eq!(model.plan, None);
        assert_eq!(model.context_window, Some(500_000));
        assert!(model.images);
        assert_eq!(model.thinking, Thinking::Effort("high".into()));
    }

    #[test]
    fn a_grok_plan_is_not_the_keyed_provider() {
        let plan = "[providers.grok]\ntype = \"grok\"\nbase_url = \"https://api.x.ai/v1\"\n";
        let (out, name, _, repaired) = with_additions(plan, &additions("xai-key")).unwrap();
        assert_eq!((name.as_str(), repaired.len()), ("xai", 0));
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(
            config.providers["grok"].kind, "grok",
            "the plan is left alone"
        );
        assert_eq!(config.providers["xai"].kind, "openai_responses");
    }
}
