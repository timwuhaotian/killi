//! Qwen on the token-plan endpoint: Alibaba's Qwen models over an OpenAI-compatible API.

use super::*;
use anyhow::Result;

/// Where Qwen's models answer.
pub const BASE_URL: &str = "https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1";

/// What `/provider qwen` writes, said before the key is asked.
pub const ABOUT: &str = "Writes a `qwen` provider on the OpenAI API at https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1 with the key, or repairs the one the config has. The key is Qwen's API key; a wrong one shows as HTTP 401 on the first request.";

/// The configured provider that is Qwen: the one on the token-plan endpoint.
pub fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://token-plan.maas.qianwenaiapi.com"))
}

/// `/provider qwen`: writes the provider and its models with `api_key`. A Qwen
/// provider the config has already is repaired: it gets the preset's API and URL.
pub fn add(api_key: &str) -> Result<Added> {
    add_models(&additions(api_key))
}

/// The provider and models the preset writes: the OpenAI API on Qwen's URL and the
/// Qwen models.
fn additions(api_key: &str) -> Additions {
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: "qwen",
        ours,
        provider,
        models: vec![
            qwen("qwen3.8-max", "Qwen3.8 Max", "xhigh"),
            qwen("qwen3.8-flash", "Qwen3.8 Flash", "high"),
        ],
        comment: "Qwen, added with /provider qwen.",
    }
}

fn qwen(id: &'static str, display: &'static str, effort: &'static str) -> NewModel {
    NewModel {
        id: id.into(),
        context: Some(1_000_000),
        output: Some(128_000),
        images: true,
        thinking: true,
        tools: true,
        display: Some(display),
        default_effort: Some(effort.into()),
        efforts: vec![effort.into()],
        ..NewModel::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preset, rendered into an empty config the way `add_models` writes it.
    fn written() -> String {
        let (out, _, _, added) = models::with_models("", &additions("sk-qwen")).unwrap();
        assert_eq!(added, ["qwen/qwen3.8-max", "qwen/qwen3.8-flash"]);
        out
    }

    #[test]
    fn the_provider_points_at_qwens_api() {
        let out = written();
        assert!(out.contains("type = \"openai\""), "{out}");
        assert!(
            out.contains("base_url = \"https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1\""),
            "{out}"
        );
    }

    #[test]
    fn a_config_without_qwen_gets_the_provider() {
        let mine = "default_model = \"openai/gpt\"\n\n[providers.openai]\ntype = \"openai\"\napi_key = \"sk\"\n\n[models.\"openai/gpt\"]\nprovider = \"openai\"\nmodel = \"gpt\"\n";
        let (out, name, added, repaired) = with_additions(mine, &additions("sk-qwen")).unwrap();
        assert_eq!((name.as_str(), added.len()), ("qwen", 2));
        assert!(repaired.is_empty(), "nothing was there to repair");
        assert!(out.starts_with(mine), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = config.resolve(Some("qwen/qwen3.8-max")).unwrap();
        assert_eq!(model.protocol, Protocol::OpenAi);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "sk-qwen");
        assert_eq!(config.default_model.as_deref(), Some("openai/gpt"));
    }

    #[test]
    fn a_qwen_provider_the_config_had_is_repaired_and_keeps_the_rest() {
        let broken = "default_model = \"qwen/qwen3.8-max\"\n\n\
            # mine\n[providers.qwen]\ntype = \"openai_responses\"\nbase_url = \"https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1\"\napi_key_env = \"QWEN_KEY\"\ncustom_headers = { x-team = \"a\" }\n\n\
            # my model\n[models.\"qwen/qwen3.8-max\"]\nprovider = \"qwen\"\nmodel = \"qwen3.8-max\"\ndefault_effort = \"xhigh\"\n";
        let (out, name, added, repaired) = with_additions(broken, &additions("sk-new")).unwrap();
        assert_eq!(name, "qwen");
        assert_eq!(added, ["qwen/qwen3.8-flash"]);
        assert_eq!(repaired, ["type"], "{out}");
        assert!(out.contains("# mine\n[providers.qwen]\n"), "{out}");
        assert!(out.contains("# my model\n"), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let qwen = &config.providers["qwen"];
        assert_eq!(qwen.kind, "openai");
        assert_eq!(
            (qwen.api_key.as_deref(), qwen.api_key_env.as_deref()),
            (Some("sk-new"), None)
        );
        assert_eq!(qwen.custom_headers["x-team"], "a");
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
    fn a_qwen_provider_on_another_api_is_left_alone() {
        let other = "[providers.qwen]\ntype = \"openai\"\nbase_url = \"https://api.openai.com/v1\"\napi_key = \"sk-openai\"\n";
        let (out, name, _, repaired) = with_additions(other, &additions("sk-qwen")).unwrap();
        assert_eq!((name.as_str(), repaired.len()), ("qwen-2", 0));
        assert!(out.contains(other), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(
            config.providers["qwen"].api_key.as_deref(),
            Some("sk-openai")
        );
        assert_eq!(
            config.providers["qwen"].base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            config.providers["qwen-2"].base_url.as_deref(),
            Some(BASE_URL)
        );
    }

    #[test]
    fn the_models_keep_qwen_capabilities_and_display_names() {
        let out = written();
        assert!(out.contains("display_name = \"Qwen3.8 Max\""), "{out}");
        assert!(out.contains("display_name = \"Qwen3.8 Flash\""), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = &config.models["qwen/qwen3.8-max"];
        assert_eq!(model.capabilities, ["image_in", "thinking", "tool_use"]);
        assert_eq!(model.default_effort.as_deref(), Some("xhigh"));
        let flash = &config.models["qwen/qwen3.8-flash"];
        assert_eq!(flash.capabilities, ["image_in", "thinking", "tool_use"]);
        assert_eq!(flash.default_effort.as_deref(), Some("high"));
    }
}
