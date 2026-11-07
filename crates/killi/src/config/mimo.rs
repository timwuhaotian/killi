//! Xiaomi's MiMo models, served over the Chat Completions API.

use super::*;
use anyhow::Result;

/// Where MiMo's models answer.
pub const BASE_URL: &str = "https://token-plan-cn.xiaomimimo.com/v1";

/// What `/provider mimo` writes, said before the key is asked.
pub const ABOUT: &str = "Writes a `mimo` provider on the Chat Completions API at https://token-plan-cn.xiaomimimo.com/v1 with the key, or repairs the one the config has; a wrong one shows as HTTP 401 on the first request.";

/// The configured provider that is MiMo: the one on Xiaomi's API.
pub fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://token-plan-cn.xiaomimimo.com"))
}

/// `/provider mimo`: writes the provider and its models with `api_key`. A MiMo
/// provider the config has already is repaired: it gets the preset's API and URL.
pub fn add(api_key: &str) -> Result<Added> {
    add_models(&additions(api_key))
}

/// The provider and models the preset writes: the Chat Completions API on Xiaomi's
/// URL and the MiMo models.
fn additions(api_key: &str) -> Additions {
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: "mimo",
        ours,
        provider,
        models: vec![mimo("mimo-v2.6-pro", "MiMo-V2.6-Pro")],
        comment: "Xiaomi MiMo, added with /provider mimo.",
    }
}

fn mimo(id: &'static str, display: &'static str) -> NewModel {
    NewModel {
        id: id.into(),
        context: Some(1_000_000),
        output: Some(128_000),
        images: true,
        thinking: true,
        tools: true,
        more_capabilities: vec!["video_in", "audio_in"],
        display: Some(display),
        ..NewModel::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preset, rendered into an empty config the way `add_models` writes it.
    fn written() -> String {
        let (out, _, _, added) = models::with_models("", &additions("sk-1")).unwrap();
        assert_eq!(added, ["mimo/mimo-v2.6-pro"]);
        out
    }

    #[test]
    fn the_provider_is_openai_on_mimos_url() {
        let out = written();
        assert!(out.contains("type = \"openai\""), "{out}");
        assert!(
            out.contains("base_url = \"https://token-plan-cn.xiaomimimo.com/v1\""),
            "{out}"
        );
    }

    #[test]
    fn a_config_without_mimo_gets_the_provider() {
        let mine = "default_model = \"openai/gpt\"\n\n[providers.openai]\ntype = \"openai\"\napi_key = \"sk\"\n\n[models.\"openai/gpt\"]\nprovider = \"openai\"\nmodel = \"gpt\"\n";
        let (out, name, added, repaired) = with_additions(mine, &additions("sk-1")).unwrap();
        assert_eq!((name.as_str(), added.len()), ("mimo", 1));
        assert!(repaired.is_empty(), "nothing was there to repair");
        assert!(out.starts_with(mine), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = config.resolve(Some("mimo/mimo-v2.6-pro")).unwrap();
        assert_eq!(model.protocol, Protocol::OpenAi);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "sk-1");
        assert_eq!(config.default_model.as_deref(), Some("openai/gpt"));
    }

    #[test]
    fn a_mimo_provider_the_config_had_is_repaired_and_keeps_the_rest() {
        let broken = "default_model = \"mimo/mimo-v2.6-pro\"\n\n\
            # mine\n[providers.mimo]\ntype = \"anthropic\"\nbase_url = \"https://token-plan-cn.xiaomimimo.com/v1\"\napi_key_env = \"MIMO_KEY\"\ncustom_headers = { x-team = \"a\" }\n\n\
            # my model\n[models.\"mimo/mimo-v2.6-pro\"]\nprovider = \"mimo\"\nmodel = \"mimo-v2.6-pro\"\n";
        let (out, name, added, repaired) = with_additions(broken, &additions("sk-2")).unwrap();
        assert_eq!(name, "mimo");
        assert!(added.is_empty(), "the model was already there");
        assert_eq!(repaired, ["type"], "{out}");
        assert!(out.contains("# mine\n[providers.mimo]\n"), "{out}");
        assert!(out.contains("# my model\n"), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let mimo = &config.providers["mimo"];
        assert_eq!(mimo.kind, "openai");
        assert_eq!(
            (mimo.api_key.as_deref(), mimo.api_key_env.as_deref()),
            (Some("sk-2"), None)
        );
        assert_eq!(mimo.custom_headers["x-team"], "a");
        let model = config.resolve(None).unwrap();
        assert_eq!(model.protocol, Protocol::OpenAi);
        assert_eq!(model.api_key, "sk-2");

        let (again, _, none, nothing) = with_additions(&out, &additions("sk-2")).unwrap();
        assert!(
            again == out && none.is_empty() && nothing.is_empty(),
            "a repaired config needs nothing more: {again}"
        );
    }

    #[test]
    fn a_mimo_provider_on_another_api_is_left_alone() {
        let other = "[providers.mimo]\ntype = \"openai\"\nbase_url = \"https://api.openai.com/v1\"\napi_key = \"sk\"\n";
        let (out, name, _, repaired) = with_additions(other, &additions("sk-1")).unwrap();
        assert_eq!((name.as_str(), repaired.len()), ("mimo-2", 0));
        assert!(out.contains(other), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(config.providers["mimo"].api_key.as_deref(), Some("sk"));
        assert_eq!(
            config.providers["mimo-2"].base_url.as_deref(),
            Some(BASE_URL)
        );
    }

    #[test]
    fn the_model_keeps_mimos_capabilities_and_display_name() {
        let out = written();
        assert!(out.contains("display_name = \"MiMo-V2.6-Pro\""), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = &config.models["mimo/mimo-v2.6-pro"];
        assert_eq!(
            model.capabilities,
            ["image_in", "thinking", "tool_use", "video_in", "audio_in"]
        );
        assert!(model.default_effort.is_none(), "{out}");
        assert!(model.support_efforts.is_empty(), "{out}");
    }
}
