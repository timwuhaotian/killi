//! Z.ai's coding API: Chat Completions on its PaaS endpoint, serving GLM coding models.

use super::*;
use anyhow::Result;

/// Where Z.ai's models answer.
pub const BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";

/// What `/provider zai` writes, said before the key is asked.
pub const ABOUT: &str = "Writes a `zai` provider on the Chat Completions API at https://api.z.ai/api/coding/paas/v4 with the key, or repairs the one the config has. The key is Z.ai's API key; a wrong one shows as HTTP 401 on the first request.";

/// The configured provider that is Z.ai: the one on Z.ai's API.
pub fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://api.z.ai"))
}

/// `/provider zai`: writes the provider and its models with `api_key`. A Z.ai
/// provider the config has already is repaired: it gets the preset's API and URL.
pub fn add(api_key: &str) -> Result<Added> {
    add_models(&additions(api_key))
}

/// The provider and models the preset writes: the Chat Completions API on Z.ai's URL
/// and the GLM coding model.
fn additions(api_key: &str) -> Additions {
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: "zai",
        ours,
        provider,
        models: vec![glm("glm-5.3", "GLM-5.3")],
        comment: "Z.ai, added with /provider zai.",
    }
}

fn glm(id: &'static str, display: &'static str) -> NewModel {
    NewModel {
        id: id.into(),
        context: Some(1_000_000),
        output: Some(128_000),
        thinking: true,
        tools: true,
        display: Some(display),
        default_effort: Some("high".into()),
        efforts: vec!["high".into()],
        ..NewModel::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preset, rendered into an empty config the way `add_models` writes it.
    fn written() -> String {
        let (out, _, _, added) = models::with_models("", &additions("sk-zai")).unwrap();
        assert_eq!(added, ["zai/glm-5.3"]);
        out
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
        let (out, name, added, repaired) = with_additions(mine, &additions("sk-zai")).unwrap();
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
        let (out, name, added, repaired) = with_additions(broken, &additions("sk-new")).unwrap();
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

        let (again, _, none, nothing) = with_additions(&out, &additions("sk-new")).unwrap();
        assert!(
            again == out && none.is_empty() && nothing.is_empty(),
            "a repaired config needs nothing more: {again}"
        );
    }

    #[test]
    fn a_zai_provider_on_another_api_is_left_alone() {
        let other = "[providers.zai]\ntype = \"openai\"\nbase_url = \"https://api.example.com/v1\"\napi_key = \"other\"\n";
        let (out, name, _, repaired) = with_additions(other, &additions("sk-zai")).unwrap();
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
    fn the_model_keeps_glm_capabilities_and_display_name() {
        let out = written();
        assert!(out.contains("display_name = \"GLM-5.3\""), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = &config.models["zai/glm-5.3"];
        assert_eq!(model.capabilities, ["thinking", "tool_use"]);
        assert_eq!(model.default_effort.as_deref(), Some("high"));
        assert_eq!(model.support_efforts, ["high"]);
    }
}
