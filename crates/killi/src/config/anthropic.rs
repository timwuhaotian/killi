//! Anthropic's Messages API: killi sends `type = "anthropic"` providers to
//! https://api.anthropic.com unless they name another URL, so the preset writes no
//! `base_url`. A provider with another URL is another API's, MiniMax's say, not ours.

use super::*;
use anyhow::Result;

/// What `/provider anthropic` writes, said before the key is asked.
pub const ABOUT: &str = "Writes an `anthropic` provider on Anthropic's Messages API at https://api.anthropic.com with the key, or repairs the one the config has. The key is Anthropic Console's `sk-ant-…`; a wrong one shows as HTTP 401 on the first request.";

/// The configured provider that is Anthropic: the `anthropic` one on Anthropic's API.
pub fn ours(provider: &Provider) -> bool {
    provider.kind == "anthropic"
        && provider
            .base_url
            .as_deref()
            .is_none_or(|url| url.starts_with("https://api.anthropic.com"))
}

/// `/provider anthropic`: writes the provider and its models with `api_key`. An Anthropic
/// provider the config has already is repaired: it gets the preset's API and key.
pub fn add(api_key: &str) -> Result<Added> {
    add_models(&additions(api_key))
}

/// The provider and models the preset writes: Anthropic's API at killi's default URL and
/// the Claude model.
fn additions(api_key: &str) -> Additions {
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "anthropic".into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: "anthropic",
        ours,
        provider,
        models: vec![sonnet("claude-sonnet-5-5", "Claude Sonnet 5.5")],
        comment: "Anthropic, added with /provider anthropic.",
    }
}

fn sonnet(id: &'static str, display: &'static str) -> NewModel {
    NewModel {
        id: id.into(),
        context: Some(1_000_000),
        output: Some(128_000),
        images: true,
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
        let (out, _, _, added) = models::with_models("", &additions("sk-ant-k")).unwrap();
        assert_eq!(added, ["anthropic/claude-sonnet-5-5"]);
        out
    }

    #[test]
    fn the_provider_has_no_url_killi_defaults_it() {
        let out = written();
        assert!(out.contains("type = \"anthropic\""), "{out}");
        assert!(!out.contains("base_url"), "{out}");
    }

    #[test]
    fn a_config_without_anthropic_gets_the_provider() {
        let mine = "default_model = \"openai/gpt\"\n\n[providers.openai]\ntype = \"openai\"\napi_key = \"sk\"\n\n[models.\"openai/gpt\"]\nprovider = \"openai\"\nmodel = \"gpt\"\n";
        let (out, name, added, repaired) = with_additions(mine, &additions("sk-ant-k")).unwrap();
        assert_eq!((name.as_str(), added.len()), ("anthropic", 1));
        assert!(repaired.is_empty(), "nothing was there to repair");
        assert!(out.starts_with(mine), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = config
            .resolve(Some("anthropic/claude-sonnet-5-5"))
            .unwrap();
        assert_eq!(model.protocol, Protocol::Anthropic);
        assert_eq!(model.base_url, "https://api.anthropic.com");
        assert_eq!(model.api_key, "sk-ant-k");
        assert_eq!(config.default_model.as_deref(), Some("openai/gpt"));
    }

    #[test]
    fn an_anthropic_provider_the_config_had_is_repaired_and_keeps_the_rest() {
        let broken = "default_model = \"anthropic/claude-sonnet-5-5\"\n\n\
            # mine\n[providers.anthropic]\ntype = \"anthropic\"\napi_key_env = \"ANTHROPIC_KEY\"\ncustom_headers = { x-team = \"a\" }\n\n\
            # my model\n[models.\"anthropic/claude-sonnet-5-5\"]\nprovider = \"anthropic\"\nmodel = \"claude-sonnet-5-5\"\ndefault_effort = \"high\"\n";
        let (out, name, added, repaired) = with_additions(broken, &additions("sk-ant-new")).unwrap();
        assert_eq!(name, "anthropic");
        assert!(added.is_empty(), "the model was already there: {added:?}");
        assert!(repaired.is_empty(), "the key isn't listed as repaired: {out}");
        assert!(out.contains("# mine\n[providers.anthropic]\n"), "{out}");
        assert!(out.contains("# my model\n"), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let anthropic = &config.providers["anthropic"];
        assert_eq!(
            (anthropic.api_key.as_deref(), anthropic.api_key_env.as_deref()),
            (Some("sk-ant-new"), None)
        );
        assert_eq!(anthropic.custom_headers["x-team"], "a");
        let model = config.resolve(None).unwrap();
        assert_eq!(model.protocol, Protocol::Anthropic);
        assert_eq!(model.base_url, "https://api.anthropic.com");
        assert_eq!(model.api_key, "sk-ant-new");

        let (again, _, none, nothing) = with_additions(&out, &additions("sk-ant-new")).unwrap();
        assert!(
            again == out && none.is_empty() && nothing.is_empty(),
            "a repaired config needs nothing more: {again}"
        );
    }

    #[test]
    fn an_anthropic_typed_provider_on_another_api_is_left_alone() {
        let minimax = "[providers.anthropic]\ntype = \"anthropic\"\nbase_url = \"https://api.minimaxi.com/anthropic\"\napi_key = \"mm\"\n";
        let (out, name, _, repaired) = with_additions(minimax, &additions("sk-ant-k")).unwrap();
        assert_eq!((name.as_str(), repaired.len()), ("anthropic-2", 0));
        assert!(out.contains(minimax), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(config.providers["anthropic"].api_key.as_deref(), Some("mm"));
        assert_eq!(
            config.providers["anthropic"].base_url.as_deref(),
            Some("https://api.minimaxi.com/anthropic")
        );
        assert_eq!(
            config.providers["anthropic-2"].api_key.as_deref(),
            Some("sk-ant-k")
        );
    }

    #[test]
    fn the_model_keeps_its_capabilities_and_display_name() {
        let out = written();
        assert!(out.contains("display_name = \"Claude Sonnet 5.5\""), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = &config.models["anthropic/claude-sonnet-5-5"];
        assert_eq!(model.capabilities, ["image_in", "thinking", "tool_use"]);
        assert_eq!(model.default_effort.as_deref(), Some("high"));
    }
}
