//! Google's Gemini models on the Gemini API's OpenAI-compatible endpoint
//! (ai.google.dev/gemini-api/docs/openai), with an API key from Google AI Studio. Google
//! supports only an API key for third-party agents: signing in with Gemini CLI's Google
//! login from another tool breaks its terms, so killi has no `killi login` for Gemini.

use super::*;
use anyhow::Result;

/// Where Gemini's OpenAI-compatible endpoint answers.
pub const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";

/// What `/provider gemini` writes, said before the key is asked.
pub const ABOUT: &str = "Writes a `gemini` provider on Gemini's OpenAI-compatible API at https://generativelanguage.googleapis.com/v1beta/openai with the key from https://aistudio.google.com/apikey, or repairs the one the config has; a wrong one shows as HTTP 400 on the first request.";

/// The configured provider that is Gemini: the one on the Gemini API.
pub fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://generativelanguage.googleapis.com"))
}

/// `/provider gemini`: writes the provider and its models with `api_key`. A Gemini provider
/// the config has already is repaired: it gets the preset's API and URL.
pub fn add(api_key: &str) -> Result<Added> {
    add_models(&additions(api_key))
}

fn additions(api_key: &str) -> Additions {
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: "gemini".into(),
        ours,
        provider,
        models: vec![
            gemini("gemini-4-carbon", "Gemini 4 Carbon", false),
            gemini("gemini-4-neon", "Gemini 4 Neon", true),
            gemini("gemini-4-hydrogen", "Gemini 4 Hydrogen", true),
            gemini("gemini-4-argon", "Gemini 4 Argon", false),
        ],
        comment: "Gemini, added with /provider gemini.",
    }
}

/// A Gemini 4 model as ai.google.dev/gemini-api/docs/models lists it: 1M tokens in, 64K out,
/// reasoning that can't be switched off, and `xhigh` on the models that take it.
fn gemini(id: &str, display: &'static str, xhigh: bool) -> NewModel {
    let mut efforts: Vec<String> = ["low", "medium", "high"].map(Into::into).into();
    if xhigh {
        efforts.push("xhigh".into());
    }
    NewModel {
        id: id.into(),
        context: Some(1_048_576),
        output: Some(65_536),
        images: true,
        thinking: true,
        tools: true,
        more_capabilities: vec!["always_thinking"],
        display: Some(display),
        default_effort: Some("high".into()),
        efforts,
        ..NewModel::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provider_speaks_openai_on_geminis_endpoint() {
        let (out, name, added, _) = with_additions("", &additions("AIza-key")).unwrap();
        assert_eq!(name, "gemini");
        assert_eq!(added.len(), 4, "{added:?}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = config.resolve(Some("gemini/gemini-4-neon")).unwrap();
        assert_eq!(model.protocol, Protocol::OpenAi);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "AIza-key");
        assert_eq!(model.max_output, Some(65_536));
        assert!(
            config.models["gemini/gemini-4-neon"]
                .support_efforts
                .contains(&"xhigh".to_string())
        );
        assert!(
            !config.models["gemini/gemini-4-carbon"]
                .support_efforts
                .contains(&"xhigh".to_string())
        );
    }

    #[test]
    fn a_gemini_provider_import_wrote_is_repaired_and_keeps_its_key() {
        let imported = "[providers.gemini]\ntype = \"openai_responses\"\nbase_url = \"https://generativelanguage.googleapis.com/v1beta/openai\"\napi_key_env = \"GEMINI_API_KEY\"\n";
        let (out, name, _, repaired) = with_additions(imported, &additions("AIza-new")).unwrap();
        assert_eq!(
            (name.as_str(), repaired.as_slice()),
            ("gemini", ["type".to_string()].as_slice())
        );
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(config.providers["gemini"].kind, "openai");
        assert_eq!(
            config.providers["gemini"].api_key.as_deref(),
            Some("AIza-new")
        );
    }
}
