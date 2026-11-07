//! StepFun's models on the Chat Completions API.

use super::*;
use anyhow::Result;

/// Where StepFun's models answer.
pub const BASE_URL: &str = "https://api.stepfun.ai/step_plan/v1";

/// What `/provider stepfun` writes, said before the key is asked.
pub const ABOUT: &str = "Writes a `stepfun` provider on the Chat Completions API at https://api.stepfun.ai/step_plan/v1 with the key, or repairs the one the config has; a wrong one shows as HTTP 401 on the first request.";

/// The configured provider that is StepFun: the one on StepFun's API.
pub fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://api.stepfun.ai"))
}

/// `/provider stepfun`: writes the provider and its models with `api_key`. A StepFun
/// provider the config has already is repaired: it gets the preset's API and URL.
pub fn add(api_key: &str) -> Result<Added> {
    add_models(&additions(api_key))
}

/// The provider and models the preset writes: the Chat Completions API on StepFun's URL
/// and the StepFun models.
fn additions(api_key: &str) -> Additions {
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: "stepfun",
        ours,
        provider,
        models: vec![step("step-5-preview", "Step-5-Preview (StepFun)")],
        comment: "StepFun, added with /provider stepfun.",
    }
}

fn step(id: &'static str, display: &'static str) -> NewModel {
    NewModel {
        id: id.into(),
        context: Some(1_000_000),
        output: Some(1_000_000),
        images: true,
        thinking: true,
        tools: true,
        more_capabilities: vec!["video_in"],
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
        let (out, _, _, added) = models::with_models("", &additions("sk-stepfun")).unwrap();
        assert_eq!(added, ["stepfun/step-5-preview"]);
        out
    }

    #[test]
    fn the_provider_is_on_stepfuns_api() {
        let out = written();
        assert!(out.contains("type = \"openai\""), "{out}");
        assert!(
            out.contains("base_url = \"https://api.stepfun.ai/step_plan/v1\""),
            "{out}"
        );
    }

    #[test]
    fn a_config_without_stepfun_gets_the_provider() {
        let mine = "default_model = \"openai/gpt\"\n\n[providers.openai]\ntype = \"openai\"\napi_key = \"sk\"\n\n[models.\"openai/gpt\"]\nprovider = \"openai\"\nmodel = \"gpt\"\n";
        let (out, name, added, repaired) = with_additions(mine, &additions("sk-stepfun")).unwrap();
        assert_eq!((name.as_str(), added.len()), ("stepfun", 1));
        assert!(repaired.is_empty(), "nothing was there to repair");
        assert!(out.starts_with(mine), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = config.resolve(Some("stepfun/step-5-preview")).unwrap();
        assert_eq!(model.protocol, Protocol::OpenAi);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "sk-stepfun");
        assert_eq!(config.default_model.as_deref(), Some("openai/gpt"));
    }

    #[test]
    fn a_stepfun_provider_the_config_had_is_repaired_and_keeps_the_rest() {
        let broken = "default_model = \"stepfun/step-5-preview\"\n\n\
            # mine\n[providers.stepfun]\ntype = \"openai_responses\"\nbase_url = \"https://api.stepfun.ai/step_plan/v1\"\napi_key_env = \"STEPFUN_KEY\"\ncustom_headers = { x-team = \"a\" }\n\n\
            # my model\n[models.\"stepfun/step-5-preview\"]\nprovider = \"stepfun\"\nmodel = \"step-5-preview\"\ndefault_effort = \"high\"\n";
        let (out, name, added, repaired) = with_additions(broken, &additions("sk-new")).unwrap();
        assert_eq!(name, "stepfun");
        assert!(added.is_empty(), "the model was already there: {out}");
        assert_eq!(repaired, ["type"], "{out}");
        assert!(out.contains("# mine\n[providers.stepfun]\n"), "{out}");
        assert!(out.contains("# my model\n"), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let stepfun = &config.providers["stepfun"];
        assert_eq!(stepfun.kind, "openai");
        assert_eq!(
            (stepfun.api_key.as_deref(), stepfun.api_key_env.as_deref()),
            (Some("sk-new"), None)
        );
        assert_eq!(stepfun.custom_headers["x-team"], "a");
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
    fn a_stepfun_provider_on_another_api_is_left_alone() {
        let other = "[providers.stepfun]\ntype = \"openai\"\nbase_url = \"https://api.openai.com/v1\"\napi_key = \"sk\"\n";
        let (out, name, _, repaired) = with_additions(other, &additions("sk-stepfun")).unwrap();
        assert_eq!((name.as_str(), repaired.len()), ("stepfun-2", 0));
        assert!(out.contains(other), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(config.providers["stepfun"].api_key.as_deref(), Some("sk"));
        assert_eq!(
            config.providers["stepfun-2"].base_url.as_deref(),
            Some(BASE_URL)
        );
    }

    #[test]
    fn the_model_keeps_stepfun_capabilities_and_display_name() {
        let out = written();
        assert!(
            out.contains("display_name = \"Step-5-Preview (StepFun)\""),
            "{out}"
        );
        let config: Config = toml::from_str(&out).unwrap();
        let model = &config.models["stepfun/step-5-preview"];
        assert_eq!(
            model.capabilities,
            ["image_in", "thinking", "tool_use", "video_in"]
        );
        assert_eq!(model.default_effort.as_deref(), Some("high"));
    }
}
