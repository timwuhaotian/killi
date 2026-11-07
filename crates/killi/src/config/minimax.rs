//! MiniMax's models on its Anthropic-compatible API.

use super::*;
use anyhow::Result;

/// Where MiniMax's models answer.
pub const BASE_URL: &str = "https://api.minimaxi.com/anthropic";

/// What `/provider minimax` writes, said before the key is asked.
pub const ABOUT: &str = "Writes a `minimax` provider on the Anthropic API at https://api.minimaxi.com/anthropic with the key, or repairs the one the config has; a wrong one shows as HTTP 401 on the first request.";

/// The configured provider that is MiniMax: the one on MiniMax's API.
pub fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://api.minimaxi.com"))
}

/// `/provider minimax`: writes the provider and its models with `api_key`. A MiniMax
/// provider the config has already is repaired: it gets the preset's API and URL.
pub fn add(api_key: &str) -> Result<Added> {
    add_models(&additions(api_key))
}

/// The provider and models the preset writes: the Anthropic API on MiniMax's URL and the
/// MiniMax model.
fn additions(api_key: &str) -> Additions {
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "anthropic".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: "minimax",
        ours,
        provider,
        models: vec![flash()],
        comment: "MiniMax, added with /provider minimax.",
    }
}

fn flash() -> NewModel {
    NewModel {
        id: "MiniMax-M3.1-Flash".into(),
        context: Some(1_000_000),
        output: Some(128_000),
        images: true,
        thinking: true,
        tools: true,
        more_capabilities: vec!["video_in"],
        display: Some("MiniMax-M3.1-Flash"),
        default_effort: Some("max".into()),
        efforts: vec!["max".into()],
        ..NewModel::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preset, rendered into an empty config the way `add_models` writes it.
    fn written() -> String {
        let (out, _, _, added) = models::with_models("", &additions("mm-key-1")).unwrap();
        assert_eq!(added, ["minimax/MiniMax-M3.1-Flash"]);
        out
    }

    #[test]
    fn the_provider_writes_the_anthropic_api_on_minimaxs_url() {
        let out = written();
        assert!(out.contains("type = \"anthropic\""), "{out}");
        assert!(
            out.contains("base_url = \"https://api.minimaxi.com/anthropic\""),
            "{out}"
        );
    }

    #[test]
    fn a_config_without_minimax_gets_the_provider() {
        let mine = "default_model = \"openai/gpt\"\n\n[providers.openai]\ntype = \"openai\"\napi_key = \"sk\"\n\n[models.\"openai/gpt\"]\nprovider = \"openai\"\nmodel = \"gpt\"\n";
        let (out, name, added, repaired) = with_additions(mine, &additions("mm-key-1")).unwrap();
        assert_eq!((name.as_str(), added.len()), ("minimax", 1));
        assert!(repaired.is_empty(), "nothing was there to repair");
        assert!(out.starts_with(mine), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = config.resolve(Some("minimax/MiniMax-M3.1-Flash")).unwrap();
        assert_eq!(model.protocol, Protocol::Anthropic);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "mm-key-1");
        assert_eq!(config.default_model.as_deref(), Some("openai/gpt"));
    }

    #[test]
    fn a_minimax_provider_the_config_had_is_repaired_and_keeps_the_rest() {
        let broken = "default_model = \"minimax/MiniMax-M3.1-Flash\"\n\n\
            # mine\n[providers.minimax]\ntype = \"openai\"\nbase_url = \"https://api.minimaxi.com/anthropic\"\napi_key_env = \"MINIMAX_KEY\"\ncustom_headers = { x-team = \"a\" }\n\n\
            # my model\n[models.\"minimax/MiniMax-M3.1-Flash\"]\nprovider = \"minimax\"\nmodel = \"MiniMax-M3.1-Flash\"\ndefault_effort = \"max\"\n";
        let (out, name, added, repaired) = with_additions(broken, &additions("mm-key-2")).unwrap();
        assert_eq!(name, "minimax");
        assert!(added.is_empty(), "the model was already there: {out}");
        assert_eq!(repaired, ["type"], "{out}");
        assert!(out.contains("# mine\n[providers.minimax]\n"), "{out}");
        assert!(out.contains("# my model\n"), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let minimax = &config.providers["minimax"];
        assert_eq!(minimax.kind, "anthropic");
        assert_eq!(
            (minimax.api_key.as_deref(), minimax.api_key_env.as_deref()),
            (Some("mm-key-2"), None)
        );
        assert_eq!(minimax.custom_headers["x-team"], "a");
        let model = config.resolve(None).unwrap();
        assert_eq!(model.protocol, Protocol::Anthropic);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "mm-key-2");

        let (again, _, none, nothing) = with_additions(&out, &additions("mm-key-2")).unwrap();
        assert!(
            again == out && none.is_empty() && nothing.is_empty(),
            "a repaired config needs nothing more: {again}"
        );
    }

    #[test]
    fn a_minimax_provider_on_another_api_is_left_alone() {
        let other = "[providers.minimax]\ntype = \"openai\"\nbase_url = \"https://api.other.com/v1\"\napi_key = \"other\"\n";
        let (out, name, _, repaired) = with_additions(other, &additions("mm-key-1")).unwrap();
        assert_eq!((name.as_str(), repaired.len()), ("minimax-2", 0));
        assert!(out.contains(other), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(config.providers["minimax"].api_key.as_deref(), Some("other"));
        assert_eq!(config.providers["minimax"].kind, "openai");
        assert_eq!(config.providers["minimax-2"].kind, "anthropic");
        assert_eq!(
            config.providers["minimax-2"].base_url.as_deref(),
            Some(BASE_URL)
        );
    }

    #[test]
    fn the_model_keeps_minimax_capabilities_and_display_name() {
        let out = written();
        assert!(
            out.contains("display_name = \"MiniMax-M3.1-Flash\""),
            "{out}"
        );
        let config: Config = toml::from_str(&out).unwrap();
        let model = &config.models["minimax/MiniMax-M3.1-Flash"];
        assert_eq!(
            model.capabilities,
            ["image_in", "thinking", "tool_use", "video_in"]
        );
        assert_eq!(model.default_effort.as_deref(), Some("max"));
    }
}
