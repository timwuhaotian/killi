//! Unisound's MaaS API: OpenAI-compatible models on `maas-api.unisound.com`.

use super::*;
use anyhow::Result;

/// Where Unisound's models answer.
pub const BASE_URL: &str = "https://maas-api.unisound.com/v1";

/// What `/provider unisound` writes, said before the key is asked.
pub const ABOUT: &str = "Writes a `unisound` provider on the OpenAI API at https://maas-api.unisound.com/v1 with the key, or repairs the one the config has; a wrong one shows as HTTP 401 on the first request.";

/// The configured provider that is Unisound: the one on Unisound's API.
pub fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://maas-api.unisound.com"))
}

/// `/provider unisound`: writes the provider and its models with `api_key`. A Unisound
/// provider the config has already is repaired: it gets the preset's API and URL.
pub fn add(api_key: &str) -> Result<Added> {
    add_models(&additions(api_key))
}

/// The provider and models the preset writes: the OpenAI API on Unisound's URL and the U2 model.
fn additions(api_key: &str) -> Additions {
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: "unisound",
        ours,
        provider,
        models: vec![u2("u2-flash", "U2 Flash (Unisound)")],
        comment: "Unisound, added with /provider unisound.",
    }
}

fn u2(id: &'static str, display: &'static str) -> NewModel {
    NewModel {
        id: id.into(),
        context: Some(1_000_000),
        output: Some(128_000),
        thinking: true,
        tools: true,
        more_capabilities: vec!["always_thinking"],
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
        let (out, _, _, added) = models::with_models("", &additions("sk-test")).unwrap();
        assert_eq!(added, ["unisound/u2-flash"]);
        out
    }

    #[test]
    fn the_provider_is_openai_on_unisounds_url() {
        let out = written();
        assert!(out.contains("type = \"openai\""), "{out}");
        assert!(
            out.contains("base_url = \"https://maas-api.unisound.com/v1\""),
            "{out}"
        );
    }

    #[test]
    fn a_config_without_unisound_gets_the_provider() {
        let mine = "default_model = \"openai/gpt\"\n\n[providers.openai]\ntype = \"openai\"\napi_key = \"sk\"\n\n[models.\"openai/gpt\"]\nprovider = \"openai\"\nmodel = \"gpt\"\n";
        let (out, name, added, repaired) = with_additions(mine, &additions("sk-test")).unwrap();
        assert_eq!((name.as_str(), added.len()), ("unisound", 1));
        assert!(repaired.is_empty(), "nothing was there to repair");
        assert!(out.starts_with(mine), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = config.resolve(Some("unisound/u2-flash")).unwrap();
        assert_eq!(model.protocol, Protocol::OpenAi);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "sk-test");
        assert_eq!(config.default_model.as_deref(), Some("openai/gpt"));
    }

    #[test]
    fn a_unisound_provider_the_config_had_is_repaired_and_keeps_the_rest() {
        let broken = "default_model = \"unisound/u2-flash\"\n\n\
            # mine\n[providers.unisound]\ntype = \"anthropic\"\nbase_url = \"https://maas-api.unisound.com/v1\"\napi_key_env = \"UNISOUND_KEY\"\ncustom_headers = { x-team = \"a\" }\n\n\
            # my model\n[models.\"unisound/u2-flash\"]\nprovider = \"unisound\"\nmodel = \"u2-flash\"\ndefault_effort = \"high\"\n";
        let (out, name, added, repaired) = with_additions(broken, &additions("sk-new")).unwrap();
        assert_eq!(name, "unisound");
        assert!(added.is_empty(), "the model was already there: {added:?}");
        assert_eq!(repaired, ["type"], "{out}");
        assert!(out.contains("# mine\n[providers.unisound]\n"), "{out}");
        assert!(out.contains("# my model\n"), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let unisound = &config.providers["unisound"];
        assert_eq!(unisound.kind, "openai");
        assert_eq!(
            (unisound.api_key.as_deref(), unisound.api_key_env.as_deref()),
            (Some("sk-new"), None)
        );
        assert_eq!(unisound.custom_headers["x-team"], "a");
        let model = config.resolve(None).unwrap();
        assert_eq!(model.protocol, Protocol::OpenAi);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "sk-new");

        let (again, _, none, nothing) = with_additions(&out, &additions("sk-new")).unwrap();
        assert!(
            again == out && none.is_empty() && nothing.is_empty(),
            "a repaired config needs nothing more: {again}"
        );
    }

    #[test]
    fn a_unisound_provider_on_another_api_is_left_alone() {
        let other = "[providers.unisound]\ntype = \"openai\"\nbase_url = \"https://api.example.com/v1\"\napi_key = \"other\"\n";
        let (out, name, _, repaired) = with_additions(other, &additions("sk-test")).unwrap();
        assert_eq!((name.as_str(), repaired.len()), ("unisound-2", 0));
        assert!(out.contains(other), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(
            config.providers["unisound"].api_key.as_deref(),
            Some("other")
        );
        assert_eq!(
            config.providers["unisound-2"].base_url.as_deref(),
            Some(BASE_URL)
        );
    }

    #[test]
    fn the_model_keeps_unisound_capabilities_and_display_name() {
        let out = written();
        assert!(
            out.contains("display_name = \"U2 Flash (Unisound)\""),
            "{out}"
        );
        let config: Config = toml::from_str(&out).unwrap();
        let model = &config.models["unisound/u2-flash"];
        assert_eq!(
            model.capabilities,
            ["thinking", "tool_use", "always_thinking"]
        );
        assert_eq!(model.default_effort.as_deref(), Some("high"));
    }
}
