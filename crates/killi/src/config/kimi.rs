//! Kimi's coding API: its coding models on the OpenAI API.

use super::*;
use anyhow::Result;

/// Where Kimi's models answer.
pub const BASE_URL: &str = "https://api.kimi.com/coding/v1";

/// What `/provider kimi` writes, said before the key is asked.
pub const ABOUT: &str = "Writes a `kimi` provider on the OpenAI API at https://api.kimi.com/coding/v1 with the key, or repairs the one the config has; a wrong one shows as HTTP 401 on the first request.";

/// The configured provider that is Kimi: the one on Kimi's API.
pub fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://api.kimi.com"))
}

/// `/provider kimi`: writes the provider and its models with `api_key`. A Kimi
/// provider the config has already is repaired: it gets the preset's API and URL.
pub fn add(api_key: &str) -> Result<Added> {
    add_models(&additions(api_key))
}

/// The provider and models the preset writes: the OpenAI API on Kimi's URL and the Kimi model.
fn additions(api_key: &str) -> Additions {
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: "kimi",
        ours,
        provider,
        models: vec![k3("k3.1-pro", "K3.1 Pro")],
        comment: "Kimi, added with /provider kimi.",
    }
}

fn k3(id: &'static str, display: &'static str) -> NewModel {
    NewModel {
        id: id.into(),
        context: Some(1_048_576),
        images: true,
        thinking: true,
        tools: true,
        more_capabilities: vec!["always_thinking", "video_in"],
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
        let (out, _, _, added) = models::with_models("", &additions("sk-kimi")).unwrap();
        assert_eq!(added, ["kimi/k3.1-pro"]);
        out
    }

    #[test]
    fn the_provider_points_at_kimis_api() {
        let out = written();
        assert!(out.contains("type = \"openai\""), "{out}");
        assert!(
            out.contains("base_url = \"https://api.kimi.com/coding/v1\""),
            "{out}"
        );
    }

    #[test]
    fn a_config_without_kimi_gets_the_provider() {
        let mine = "default_model = \"openai/gpt\"\n\n[providers.openai]\ntype = \"openai\"\napi_key = \"sk\"\n\n[models.\"openai/gpt\"]\nprovider = \"openai\"\nmodel = \"gpt\"\n";
        let (out, name, added, repaired) = with_additions(mine, &additions("sk-new")).unwrap();
        assert_eq!((name.as_str(), added.len()), ("kimi", 1));
        assert!(repaired.is_empty(), "nothing was there to repair");
        assert!(out.starts_with(mine), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = config.resolve(Some("kimi/k3.1-pro")).unwrap();
        assert_eq!(model.protocol, Protocol::OpenAi);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "sk-new");
        assert_eq!(config.default_model.as_deref(), Some("openai/gpt"));
    }

    #[test]
    fn a_kimi_provider_the_config_had_is_repaired_and_keeps_the_rest() {
        let broken = "default_model = \"kimi/k3.1-pro\"\n\n\
            # mine\n[providers.kimi]\ntype = \"openai_responses\"\nbase_url = \"https://api.kimi.com/coding/v1\"\napi_key_env = \"KIMI_KEY\"\ncustom_headers = { x-team = \"a\" }\n\n\
            # my model\n[models.\"kimi/k3.1-pro\"]\nprovider = \"kimi\"\nmodel = \"k3.1-pro\"\ndefault_effort = \"high\"\n";
        let (out, name, added, repaired) = with_additions(broken, &additions("sk-new")).unwrap();
        assert_eq!(name, "kimi");
        assert!(added.is_empty(), "the model was already there: {out}");
        assert_eq!(repaired, ["type"], "{out}");
        assert!(out.contains("# mine\n[providers.kimi]\n"), "{out}");
        assert!(out.contains("# my model\n"), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let kimi = &config.providers["kimi"];
        assert_eq!(kimi.kind, "openai");
        assert_eq!(
            (kimi.api_key.as_deref(), kimi.api_key_env.as_deref()),
            (Some("sk-new"), None)
        );
        assert_eq!(kimi.custom_headers["x-team"], "a");
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
    fn a_kimi_provider_on_another_api_is_left_alone() {
        let llama = "[providers.kimi]\ntype = \"openai\"\nbase_url = \"https://api.llama.com/v1\"\napi_key = \"llama\"\n";
        let (out, name, _, repaired) = with_additions(llama, &additions("sk-k")).unwrap();
        assert_eq!((name.as_str(), repaired.len()), ("kimi-2", 0));
        assert!(out.contains(llama), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(config.providers["kimi"].api_key.as_deref(), Some("llama"));
        assert_eq!(config.providers["kimi"].kind, "openai");
        assert_eq!(
            config.providers["kimi-2"].base_url.as_deref(),
            Some(BASE_URL)
        );
    }

    #[test]
    fn the_models_keep_kimi_capabilities_and_display_names() {
        let out = written();
        assert!(out.contains("display_name = \"K3.1 Pro\""), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = &config.models["kimi/k3.1-pro"];
        assert_eq!(
            model.capabilities,
            [
                "image_in",
                "thinking",
                "tool_use",
                "always_thinking",
                "video_in"
            ]
        );
        assert_eq!(model.default_effort.as_deref(), Some("high"));
    }
}
